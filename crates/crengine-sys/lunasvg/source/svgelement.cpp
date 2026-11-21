#include "svgelement.h"
#include "parser.h"
#include "layoutcontext.h"

namespace lunasvg {

SVGElement::SVGElement()
    : GraphicsElement(ElementID::Svg)
{
}

Length SVGElement::x() const
{
    auto& value = get(PropertyID::X);
    return Parser::parseLength(value, AllowNegativeLengths, Length::Zero);
}

Length SVGElement::y() const
{
    auto& value = get(PropertyID::Y);
    return Parser::parseLength(value, AllowNegativeLengths, Length::Zero);
}

Length SVGElement::width() const
{
    auto& value = get(PropertyID::Width);
    return Parser::parseLength(value, ForbidNegativeLengths, Length::HundredPercent);
}

Length SVGElement::height() const
{
    auto& value = get(PropertyID::Height);
    return Parser::parseLength(value, ForbidNegativeLengths, Length::HundredPercent);
}

Rect SVGElement::viewBox() const
{
    auto& value = get(PropertyID::ViewBox);
    return Parser::parseViewBox(value);
}

PreserveAspectRatio SVGElement::preserveAspectRatio() const
{
    auto& value = get(PropertyID::PreserveAspectRatio);
    return Parser::parsePreserveAspectRatio(value);
}

std::unique_ptr<LayoutSymbol> SVGElement::build(const TreeBuilder* builder)
{
    if(isDisplayNone())
        return nullptr;

    auto w = this->width();
    auto h = this->height();
    if(w.isZero() || h.isZero())
        return nullptr;

    LengthContext lengthContext(this);
    auto _x = lengthContext.valueForLength(x(), LengthMode::Width);
    auto _y = lengthContext.valueForLength(y(), LengthMode::Height);
    auto _w = lengthContext.valueForLength(w, LengthMode::Width);
    auto _h = lengthContext.valueForLength(h, LengthMode::Height);

    auto viewBox = this->viewBox();

    if ( builder->hasExternalContext() ) {
        external_context_t * xcontext = builder->getExternalContext();
        if ( xcontext->target_width >= 0 && xcontext->target_height >= 0) {
            // Force this SVG to have the requested width and height
            if ((w.isRelative() || h.isRelative()) && !has(PropertyID::ViewBox)) {
                // For this edge case (no width/height/viewBox), the code at the bottom of
                // this function will compute the advertised native width and height from
                // the stroke bounding box. For our target size tweaks to work, we need
                // to use these same values to create the missing viewBox.
                // A little hacky solution, that seems to work without side effects, is
                // to just call again this here function without having it going through
                // this target sizes tweaks. (This will double the time needed to build
                // this SVG, but it should be fine with small ones, which are the ones
                // that may make that edge case.)
                int target_width = xcontext->target_width; // backup target sizes
                int target_height = xcontext->target_height;
                xcontext->target_width = -1; // pretend we don't request them
                xcontext->target_height = -1;
                auto tmproot = build(builder); // rebuild (this should work with another local set of temp objects)
                _w = tmproot->width; // use the computed stroke bounding box sizes as the native sizes
                _h = tmproot->height;
                xcontext->target_width = target_width; // restore target sizes
                xcontext->target_height = target_height;
            }
            if ( viewBox.empty() ) {
                // If no viewBox= was present, the original width/height would be used as
                // the viewbox and all coordinates would relate to them.
                // We should add the missing viewBox with the original width/height we are
                // going to replace.
                //   viewBox = Rect{_x, _y, _w, _h};
                // Best to add it as a string property, as it may be fetched from
                // elsewhere (ie. Element::currentViewport()).
                std::string vb = "";
                vb += std::to_string(_x); // (should we force x=y=0 instead ?)
                vb += ' ';
                vb += std::to_string(_y);
                vb += ' ';
                vb += std::to_string(_w);
                vb += ' ';
                vb += std::to_string(_h);
                set(PropertyID::ViewBox, vb, 0x10);
                viewBox = this->viewBox();
            }
            _w = xcontext->target_width;
            _h = xcontext->target_height;
            _x = 0;
            _y = 0;
            // Not sure if setting these as properties is needed, but let's be safe
            set(PropertyID::X, std::to_string(_x), 0x10);
            set(PropertyID::Y, std::to_string(_y), 0x10);
            set(PropertyID::Width, std::to_string(_w), 0x10);
            set(PropertyID::Height, std::to_string(_h), 0x10);
        }
    }

    auto preserveAspectRatio = this->preserveAspectRatio();
    auto viewTranslation = Transform::translated(_x, _y);
    auto viewTransform = preserveAspectRatio.getMatrix(_w, _h, viewBox);

    auto root = makeUnique<LayoutSymbol>();
    root->width = _w;
    root->height = _h;
    root->transform = (viewTransform * viewTranslation) * transform();
    root->clip = isOverflowHidden() ? preserveAspectRatio.getClip(_w, _h, viewBox) : Rect::Invalid;
    root->opacity = opacity();

    LayoutContext context(builder, root.get());
    root->masker = context.getMasker(mask());
    root->clipper = context.getClipper(clip_path());
    layoutChildren(&context, root.get());
    if((w.isRelative() || h.isRelative()) && !has(PropertyID::ViewBox)) {
        auto box = root->map(root->strokeBoundingBox());
        root->width = w.value(box.x + box.w);
        root->height = h.value(box.y + box.h);
    }

    return root;
}

void SVGElement::layout(LayoutContext* context, LayoutContainer* current) const
{
    if(isDisplayNone())
        return;

    auto w = this->width();
    auto h = this->height();
    if(w.isZero() || h.isZero())
        return;

    LengthContext lengthContext(this);
    auto _x = lengthContext.valueForLength(x(), LengthMode::Width);
    auto _y = lengthContext.valueForLength(y(), LengthMode::Height);
    auto _w = lengthContext.valueForLength(w, LengthMode::Width);
    auto _h = lengthContext.valueForLength(h, LengthMode::Height);

    auto viewBox = this->viewBox();
    auto preserveAspectRatio = this->preserveAspectRatio();
    auto viewTranslation = Transform::translated(_x, _y);
    auto viewTransform = preserveAspectRatio.getMatrix(_w, _h, viewBox);

    auto symbol = makeUnique<LayoutSymbol>();
    symbol->width = _w;
    symbol->height = _h;
    symbol->transform = (viewTransform * viewTranslation) * transform();
    symbol->clip = isOverflowHidden() ? preserveAspectRatio.getClip(_w, _h, viewBox) : Rect::Invalid;
    symbol->opacity = opacity();
    symbol->masker = context->getMasker(mask());
    symbol->clipper = context->getClipper(clip_path());
    layoutChildren(context, symbol.get());
    current->addChildIfNotEmpty(std::move(symbol));
}

std::unique_ptr<Node> SVGElement::clone() const
{
    return cloneElement<SVGElement>();
}

} // namespace lunasvg
